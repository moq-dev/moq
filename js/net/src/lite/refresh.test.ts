import { expect, spyOn, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { accept } from "../connection/accept.ts";
import { connect } from "../connection/connect.ts";
import { defined } from "../defined.test-util.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { everyRange } from "../subscription.test-util.ts";
import { Timestamp } from "../time.ts";
import { SubscribeUpdate } from "./subscribe.ts";
import { ALPN_06, ALPN_07_WIP } from "./version.ts";

test.each([ALPN_06, ALPN_07_WIP])("refresh joins reach a browser publisher on %s", async (alpn) => {
	const clock = new Manual();
	const installed = clock.install();
	const source = new Producer();
	const b = new Producer();
	const broadcastRoute = source.route(Path.from("room"));
	const broadcast = broadcastRoute.broadcast;
	const producer = defined(broadcast.createTrack("data", { refresh: { ratio: 1 } }));
	const asked = hooks.demandAsked(producer.demand());
	const pair = createMockTransportPair(alpn);
	const url = new URL("https://localhost/refresh");
	const updates: bigint[] = [];
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
		const announcements = b.routes();
		while ((await announcements.next())?.kind !== "start") {}
		const request = b.request(Path.from("room"));
		const remote = (await request.broadcast()).track("data");
		expect((await remote.info()).refresh).toEqual({ ratio: alpn === ALPN_07_WIP ? 1 : null, interval: null });
		const baseline = remote.subscribe();
		const group = hooks.appendNext(producer);
		group.writeFrame({ payload: new Uint8Array(3), timestamp: Timestamp.unscaled(group.start) });
		group.writeFrame({ payload: new Uint8Array(3), timestamp: Timestamp.unscaled(group.start) });
		const received = await baseline.recvGroup();
		if (!received) throw new Error("group missing");
		expect((await received.readFrame())?.payload.length).toBe(3);
		expect((await received.readFrame())?.payload.length).toBe(3);
		const joins = [remote.subscribe()];
		await clock.advance(Duration.nanos(0));
		expect(asked.peek()).toBeUndefined();
		updates.length = 0;
		group.writeFrame({ payload: new Uint8Array(1), timestamp: Timestamp.unscaled(group.start) });
		expect((await received.readFrame())?.payload.length).toBe(1);
		for (let i = 0; i < 8; i++) joins.push(remote.subscribe());
		await clock.advance(Duration.nanos(0));
		if (alpn === ALPN_07_WIP) {
			while (asked.peek() !== 1n) await asked.changed();
		}
		expect(asked.peek()).toBe(alpn === ALPN_07_WIP ? 1n : undefined);
		// Concurrent local joins coalesce into one upstream update.
		expect(updates).toEqual(alpn === ALPN_07_WIP ? [1n] : []);
		// A parameter change restates the wanted group rather than clearing it.
		joins.push(remote.subscribe(everyRange({}, { priority: 9 })));
		await clock.advance(Duration.nanos(0));
		expect(updates).toEqual(alpn === ALPN_07_WIP ? [1n, 1n] : []);
		for (const join of joins) join.close();
		received.close();
		baseline.close();
		group.finish();
	} finally {
		spy.mockRestore();
		for (const origin of [b, source]) origin.close();
		for (const peer of peers) peer.abort();
		await clock.advance(Duration.nanos(0));
		installed.close();
	}
});

// A reader behind a lite hop: its refresh waits out the interval at its own copy, resolves once the
// session sends the request, and the publisher hears it.
test("a refresh resolves once the session sends it upstream", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const source = new Producer();
	const b = new Producer();
	const producer = defined(
		source.route(Path.from("room")).broadcast.createTrack("data", {
			refresh: { interval: Duration.secs(5) },
		}),
	);
	const asked = hooks.demandAsked(producer.demand());
	const pair = createMockTransportPair(ALPN_07_WIP);
	const url = new URL("https://localhost/refresh");
	const peers: { abort(): void }[] = [];
	try {
		peers.push(
			...(await Promise.all([
				connect({ url, transport: pair.client, consume: b }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			])),
		);
		const announcements = b.routes();
		while ((await announcements.next())?.kind !== "start") {}
		const remote = (await b.request(Path.from("room")).broadcast()).track("data");
		const reader = remote.subscribe();
		const group = hooks.appendNext(producer);
		group.writeFrame({ payload: new Uint8Array(1), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
		const received = await reader.recvGroup();
		if (!received) throw new Error("group missing");

		let sent = false;
		const refresh = reader.refresh().then(() => {
			sent = true;
		});
		await clock.advance(Duration.secs(4));
		expect(sent).toBe(false);
		expect(asked.peek()).toBeUndefined();
		await clock.advance(Duration.secs(1));
		await refresh;
		while (asked.peek() !== 1n) await asked.changed();

		received.close();
		reader.close();
	} finally {
		for (const origin of [b, source]) origin.close();
		for (const peer of peers) peer.abort();
		await clock.advance(Duration.nanos(0));
		installed.close();
	}
});

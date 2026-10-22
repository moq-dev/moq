import { expect, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { accept } from "../connection/accept.ts";
import { connect } from "../connection/connect.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { ALPN_06, ALPN_07_WIP } from "./version.ts";

for (const downstream of [ALPN_06, ALPN_07_WIP]) {
	test(`compressed publishing on ${downstream}, FETCH and independent datagrams`, async () => {
		const clock = new Manual();
		const restore = clock.install();
		const source = new Producer();
		const destination = new Producer();
		const broadcast = source.createBroadcast(Path.from("compressed"));
		const track = broadcast.createTrack("data", { compress: true });
		broadcast.announce();
		const peers: { abort(): void }[] = [];
		async function link(publish: Producer, consume: Producer, alpn: string) {
			const pair = createMockTransportPair(alpn);
			const url = new URL("https://localhost/compressed");
			peers.push(
				...(await Promise.all([
					connect({ url, transport: pair.client, consume }),
					accept({ url, transport: pair.server, publish: publish.consume() }),
				])),
			);
			const announcements = consume.announced();
			try {
				while ((await announcements.next())?.kind !== "start") {}
			} finally {
				announcements.close();
			}
		}
		try {
			await link(source, destination, downstream);
			const request = destination.request(Path.from("compressed"));
			while (request.active.peek() === undefined) await request.active.changed();
			const active = request.active.peek();
			if (!active) throw new Error("broadcast did not resolve");
			const remote = active.track("data");
			expect((await remote.info()).compress).toBe(downstream === ALPN_07_WIP);
			const reader = remote.subscribe({ maxDelay: Milli(1000) });
			const group = track.appendGroup();
			const payloads = [new Uint8Array(4096).fill(7), new Uint8Array(), new Uint8Array(4096).fill(7)];
			for (const payload of payloads) group.writeFrame({ payload });
			group.close();
			const received = await reader.recvGroup();
			if (!received) throw new Error("missing group");
			expect(received.sequence).toBe(0);
			for (const payload of payloads) expect((await received.readFrame())?.payload).toEqual(payload);
			expect(await received.readFrame()).toBeUndefined();
			const fetched = await remote.fetchGroup(0);
			for (const payload of payloads) expect((await fetched.readFrame())?.payload).toEqual(payload);
			expect(await fetched.readFrame()).toBeUndefined();
			// Each datagram has a cold window; no lost or reordered predecessor is needed.
			for (const payload of [new Uint8Array(100).fill(4), new Uint8Array(100).fill(8)]) {
				track.appendDatagram(undefined, payload);
				expect((await reader.recvDatagram())?.payload).toEqual(payload);
			}
			reader.close();
			fetched.close();
			received?.close();
			request.close();
		} finally {
			for (const origin of [source, destination]) origin.close();
			for (const peer of peers) peer.abort();
			await clock.advance(Nano.zero);
			restore();
		}
	});
}

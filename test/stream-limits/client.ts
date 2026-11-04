import assert from "node:assert/strict";
import * as Moq from "../../js/net/src/index.ts";

// stdout carries the `ready` handshake to the Rust test, so library debug logs go to stderr.
console.debug = console.error;

const [address, mode, countArg] = process.argv.slice(2);
const count = Number(countArg);
if (!address || !["publish", "subscribe"].includes(mode) || !Number.isSafeInteger(count) || count <= 0)
	throw new Error("expected URL publish|subscribe COUNT");

const path = Moq.Path.from("limits");
const origin = new Moq.Origin.Producer();
// Bun has no WebTransport, so this dials the WebSocket fallback, whose qmux session sets the limits.
const url = new URL(address);

try {
	if (mode === "publish") {
		const publication = origin.createBroadcast(path);
		// One group per track, never finished, so every group the reader receives stays live.
		for (let i = 0; i < count; i++) {
			const group = publication.broadcast.createTrack(String(i)).appendGroup();
			group.writeFrame({ payload: new TextEncoder().encode(String(i)) });
		}
		publication.announce();
		const session = await Moq.Connection.connect({ url, publish: origin.consume() });
		console.log("ready");
		for await (const _ of Bun.stdin.stream()) {
			/* parent closes stdin once the reader is done */
		}
		await session.close();
	} else {
		const session = await Moq.Connection.connect({ url, consume: origin });
		const request = origin.request(path);
		let broadcast = request.active.peek();
		while (!broadcast) broadcast = await request.active.changed();
		assert(broadcast);
		const active = broadcast;
		// Each subscription is a stream to the publisher; each group, one from the relay.
		const tracks = Array.from({ length: count }, (_, i) => active.track(String(i)).subscribe());
		const groups = await Promise.all(
			tracks.map(async (track, i) => {
				const group = await track.recvGroup();
				assert(group, `track ${i} ended without a group`);
				assert.equal(await group.readString(), String(i), `track ${i}`);
				return group;
			}),
		);
		assert.equal(groups.length, count);
		for (const track of tracks) track.close();
		request.close();
		await session.close();
	}
} finally {
	origin.close();
}

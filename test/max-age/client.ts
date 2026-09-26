import assert from "node:assert/strict";
import Session from "@moq/qmux";
import { connect } from "../../js/net/src/connection/connect.ts";
import { Producer } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";
import { Milli } from "../../js/net/src/time.ts";

const [address, protocol, mode] = process.argv.slice(2);
if (!address || !protocol || !["publish", "subscribe"].includes(mode))
	throw new Error("expected URL ALPN publish|subscribe");
const origin = new Producer();
const ages = [undefined, Milli(0), Milli(30_000)];
if (mode === "publish") {
	const broadcast = origin.createBroadcast(Path.from("age"));
	for (const [i, maxAge] of ages.entries()) broadcast.createTrack(String(i), { maxAge });
	broadcast.announce();
}
const url = new URL(address);
const transport = new Session(url, {
	protocols: [protocol],
	versions: { [protocol]: protocol === "moqt-22" ? "qmux-01" : null },
	requireProtocol: true,
});
await transport.ready;
const session = await connect({
	url,
	transport,
	publish: mode === "publish" ? origin.consume() : undefined,
	consume: mode === "subscribe" ? origin : undefined,
});
try {
	if (mode === "subscribe") {
		const announcements = origin.consume().announced();
		await announcements.next();
		const request = origin.consume().request(Path.from("age"));
		let broadcast = request.active.peek();
		while (!broadcast) broadcast = await request.active.changed();
		assert(broadcast);
		for (const [i, age] of ages.entries()) {
			const track = broadcast.track(String(i)).subscribe();
			assert.equal((await track.info()).maxAge, age, `${protocol} track ${i}`);
			track.close();
		}
		request.close();
		announcements.close();
	} else {
		console.log("ready");
		for await (const _ of Bun.stdin.stream()) {
			/* parent closes stdin after assertions */
		}
	}
} finally {
	session.close();
	origin.close();
}

import assert from "node:assert/strict";
import { Duration } from "@moq/clock";
import Session from "@moq/qmux";
import { connect } from "../../js/net/src/connection/connect.ts";
import { Producer } from "../../js/net/src/origin.ts";
import * as Path from "../../js/net/src/path.ts";

// stdout carries the `ready` handshake to the Rust test, so library debug logs go to stderr.
console.debug = console.error;

const [address, protocol, mode] = process.argv.slice(2);
if (!address || !protocol || !["publish", "subscribe"].includes(mode))
	throw new Error("expected URL ALPN publish|subscribe");
const origin = new Producer();
// `null` is an explicit unlimited: an omitted max age is the zero authored default.
const ages = [null, Duration.millis(0), Duration.millis(30_000)];
if (mode === "publish") {
	const broadcastRoute = origin.route(Path.from("age"), { hidden: true });
	const broadcast = broadcastRoute.broadcast;
	for (const [i, maxAge] of ages.entries()) broadcast.createTrack(String(i), { maxAge });
	broadcastRoute.announce();
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
		// `live` can come first while the relay has yet to learn the publisher's route.
		for (;;) {
			const event = await announcements.next();
			assert(event, "announcements closed");
			if (event.kind === "present" && event.prefix === "age") break;
		}
		const request = origin.consume().request(Path.from("age"));
		let broadcast = request.active.peek();
		while (!broadcast) broadcast = await request.active.changed();
		assert(broadcast);
		for (const [i, age] of ages.entries()) {
			const track = broadcast.track(String(i)).subscribe();
			// A track received without a max age has no publisher limit: absent, not null.
			assert.equal((await track.info()).maxAge?.asMillis(), age?.asMillis(), `${protocol} track ${i}`);
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
	await session.close();
	origin.close();
}

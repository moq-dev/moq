/** Follow a Rust server's GOAWAY redirect, retaining the reader since the epoch proves continuity. */
import assert from "node:assert/strict";
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";
import { Reload } from "../../../../js/net/src/connection/reload.ts";
import { wireOf } from "../../../../js/net/src/wire.ts";

console.debug = console.error;
console.info = console.error;
const [first, second, version] = process.argv.slice(2);
assert(first && second && version, "usage: goaway.ts FIRST_URL SECOND_URL VERSION");
const origin = new Moq.Origin.Producer();
const connection = new Reload({
	url: new URL(first),
	versions: [version],
	consume: origin,
	websocket: { enabled: true, delay: Milli(0) },
	goaway: { redirect: "follow" },
});
let replacementProbe: Moq.Track.Subscriber | undefined;
const requests = [origin.follow(Moq.Path.from("handover"))];
async function active(request: (typeof requests)[number]) {
	let broadcast = request.active.peek();
	while (!broadcast) {
		await request.active.changed();
		broadcast = request.active.peek();
	}
	return broadcast;
}
try {
	const track = (await active(requests[0])).track("video").subscribe({ groups: { start: { included: 0 } } });
	try {
		for (let sequence = 0; sequence < 3; sequence++) {
			const group = await track.recvGroup();
			assert(group, "subscription ended during redirect");
			try {
				assert.equal(group.sequence, sequence, "handover duplicated or skipped a group");
				const frame = await group.readFrame();
				assert(frame, "empty group");
				assert.deepEqual(frame.payload, new Uint8Array(32).fill(sequence));
				assert.equal(await group.readFrame(), undefined, "extra frame");
			} finally {
				group.close();
			}
			if (sequence === 0) process.stdout.write("first group\n");
			if (sequence === 1) {
				let replacement = connection.established.peek();
				while (replacement?.url.href !== new URL(second).href) {
					await connection.established.changed();
					replacement = connection.established.peek();
				}
				const announcements = replacement.announced(Moq.Path.Pattern.literal("handover"));
				try {
					for (;;) {
						const event = await announcements.next();
						assert(event, "replacement announcements ended");
						if (event.kind === "start" && event.prefix === "handover") break;
					}
				} finally {
					announcements.close();
				}
				// A direct read from the replacement proves its SUBSCRIBE/data path is ready,
				// beyond SETUP or namespace discovery, while the original reader stays held.
				replacementProbe = wireOf(replacement)
					.consume(Moq.Path.from("handover"))
					.track("video")
					.subscribe({ groups: { start: { included: 1 } } });
				const probe = await replacementProbe.recvGroup();
				assert(probe, "replacement has no group");
				try {
					assert.equal(probe.sequence, 1, "replacement served the wrong group");
					assert.deepEqual((await probe.readFrame())?.payload, new Uint8Array(32).fill(1));
				} finally {
					probe.close();
				}
				process.stdout.write("replacement ready\n");
			}
		}
		// The server only writes group 2 after this acknowledgement and closing `first`.
		process.stdout.write("goaway resumed\n");
	} finally {
		track.close();
	}
} finally {
	replacementProbe?.close();
	for (const request of requests) request.close();
	connection.close();
	origin.close();
}

/** Finite raw-track interop: publish until the harness acknowledges, or read through clean EOF. @module */
import { strict as assert } from "node:assert";
import { createInterface } from "node:readline";
import { Milli } from "@moq/clock";
import * as Moq from "@moq/net";
import { install } from "@moq/web-transport";

install();
const GROUPS = 4;
const BYTES = 256 * 1024;
const [role, url, path, version, mode] = process.argv.slice(2);
const drop = mode === "drop";
if (!url || !path || !["publish", "subscribe"].includes(role)) {
	throw new Error("usage: tail.ts publish|subscribe URL BROADCAST");
}

const origin = new Moq.Origin.Producer();
const connection = await Moq.Connection.connect({
	url: new URL(url),
	websocket: { enabled: false },
	...(version ? { webtransport: { protocols: [version] } } : {}),
	...(role === "publish" ? { publish: origin.consume() } : { consume: origin }),
});
try {
	if (role === "publish") {
		const broadcast = origin.createBroadcast(Moq.Path.from(path));
		const track = broadcast.createTrack("tail");
		broadcast.announce();
		const demand = track.demand();
		while (demand.wanted.peek() === undefined) await demand.wanted.changed();
		// The end precedes the last group's data; the transport still has to drain it.
		track.finishAt({ group: GROUPS, frame: 0 });
		for (let sequence = 0; sequence < GROUPS; sequence++) {
			const group = track.appendGroup();
			if (drop && sequence === 1) {
				group.close(new Error("cut"));
				continue;
			}
			group.writeFrame({ payload: new Uint8Array(BYTES).fill(sequence) });
			group.close();
		}
		track.close();
		const lines = createInterface({ input: process.stdin });
		try {
			const ack = await lines[Symbol.asyncIterator]().next();
			assert.equal(ack.value, "clean end", "reader did not acknowledge a clean end");
		} finally {
			lines.close();
		}
		broadcast.close();
		console.error("tail acknowledged");
	} else {
		const request = origin.request(Moq.Path.from(path), { announced: true });
		try {
			let broadcast = request.active.peek();
			while (!broadcast) {
				await request.active.changed();
				broadcast = request.active.peek();
			}
			const track = broadcast
				.track("tail")
				.subscribe({ maxDelay: Milli(drop ? 60_000 : 5000), groups: { start: { included: 0 } } });
			const seen: number[] = [];
			try {
				for (;;) {
					const group = await track.recvGroup();
					if (!group) break;
					try {
						assert(group.sequence < GROUPS, `unexpected group ${group.sequence}`);
						const frame = await group.readFrame();
						assert(frame, "empty group");
						assert.equal(frame.payload.byteLength, BYTES, "truncated group");
						assert(
							frame.payload.every((byte) => byte === group.sequence),
							"corrupt group",
						);
						assert.equal(await group.readFrame(), undefined, "extra frame");
						seen.push(group.sequence);
					} finally {
						group.close();
					}
				}
				assert.deepEqual(
					seen.sort((a, b) => a - b),
					drop ? [0, 2, 3] : [0, 1, 2, 3],
				);
				assert.deepEqual(await track.finished(), { group: GROUPS, frame: 0 });
				console.error(
					drop
						? "tail drop clean end=4 groups=0,2,3 bytes=786432"
						: "tail clean end=4 groups=0,1,2,3 bytes=1048576",
				);
			} finally {
				track.close();
			}
		} finally {
			request.close();
		}
	}
} finally {
	connection.close();
	origin.close();
}
process.exit(0);

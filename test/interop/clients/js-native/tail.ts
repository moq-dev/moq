/**
 * Finite raw-track interop: publish until the harness acknowledges, or read through clean EOF.
 *
 * `drop` cuts group 1. `join` reads from whichever group the subscription starts at: a
 * moq-transport subscription starts at the largest group rather than at the range asked for, so
 * it must read a contiguous run of groups through the last one. `compress` publishes a compressed
 * track with several frames, one empty, in each group.
 *
 * @module
 */
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
const compress = mode === "compress";
console.debug = console.error;
console.info = console.error;
const join = mode === "join" || mode === "status";
const datagram = mode === "datagram";
const status = mode === "status";
if (
	!url ||
	!path ||
	!["publish", "subscribe"].includes(role) ||
	![undefined, "drop", "join", "compress", "datagram", "status"].includes(mode)
) {
	throw new Error("usage: tail.ts publish|subscribe URL BROADCAST [VERSION [drop|join|compress|datagram|status]]");
}

const origin = new Moq.Origin.Producer();
const connection = await Moq.Connection.connect({
	url: new URL(url),
	websocket: { enabled: false },
	...(version ? { versions: [version] } : {}),
	...(role === "publish" ? { publish: origin.consume() } : { consume: origin }),
});
try {
	if (role === "publish") {
		const broadcastPublication = origin.createBroadcast(Moq.Path.from(path));
		const broadcast = broadcastPublication.broadcast;
		const track = broadcast.createTrack("tail", { compress, ...(status ? { priority: 2 } : {}) });
		broadcastPublication.announce();
		const lines = createInterface({ input: process.stdin });
		const commands = lines[Symbol.asyncIterator]();
		if (!status) {
			const demand = track.demand();
			while (demand.wanted.peek() === undefined) await demand.wanted.changed();
			if (datagram) {
				for (let sequence = 0; sequence < GROUPS; sequence++) {
					assert.equal((await commands.next()).value, `datagram ${sequence}`, "missing datagram command");
					track.appendDatagram(undefined, new Uint8Array(900).fill(sequence));
				}
			} else {
				// The end precedes the last group's data; the transport still has to drain it.
				track.finishAt({ group: GROUPS, frame: 0 });
				for (let sequence = 0; sequence < GROUPS; sequence++) {
					const group = track.appendGroup();
					if (drop && sequence === 1) {
						group.abort(new Error("cut"));
						continue;
					}
					for (const size of compress ? [BYTES / 2, 0, BYTES / 2] : [BYTES]) {
						group.writeFrame({ payload: new Uint8Array(size).fill(sequence) });
					}
					group.finish();
				}
				track.finish();
				track.close();
			}
		}
		try {
			const ack = await commands.next();
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
			if (compress) assert.equal((await track.info()).compress, version === "moq-lite-07-wip");
			const seen: number[] = [];
			try {
				if (datagram) {
					await track.info();
					process.stdout.write("datagram ready\n");
					for (let sequence = 0; sequence < GROUPS; sequence++) {
						const frame = await track.recvDatagram();
						assert(frame, "missing datagram");
						assert.equal(frame.sequence, sequence, "wrong datagram sequence");
						assert.equal(frame.payload.byteLength, 900, "truncated datagram");
						assert(
							frame.payload.every((byte) => byte === sequence),
							"corrupt datagram",
						);
						assert.equal(frame.timestamp, undefined, "unexpected IETF timestamp");
						process.stdout.write(`datagram ${sequence}\n`);
					}
					console.error("datagram clean groups=0,1,2,3 bytes=3600");
				} else {
					for (;;) {
						const group = await track.recvGroup();
						if (!group) break;
						try {
							assert(group.sequence < GROUPS, `unexpected group ${group.sequence}`);
							for (const size of compress ? [BYTES / 2, 0, BYTES / 2] : [BYTES]) {
								const frame = await group.readFrame();
								assert(frame, "empty group");
								assert.equal(frame.payload.byteLength, size, "truncated group");
								assert(
									frame.payload.every((byte) => byte === group.sequence),
									"corrupt group",
								);
							}
							assert.equal(await group.readFrame(), undefined, "extra frame");
							seen.push(group.sequence);
						} finally {
							group.close();
						}
					}
					seen.sort((a, b) => a - b);
					const first = join ? (seen[0] ?? 0) : 0;
					const all = Array.from({ length: GROUPS - first }, (_, i) => first + i);
					assert.deepEqual(seen, drop ? [0, 2, 3] : all);
					assert.deepEqual(await track.finished(), { group: GROUPS, frame: 0 });
					if (join) console.error(`tail join clean end=4 groups=${seen.join(",")}`);
					else if (drop) console.error("tail drop clean end=4 groups=0,2,3 bytes=786432");
					else console.error("tail clean end=4 groups=0,1,2,3 bytes=1048576");
				}
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

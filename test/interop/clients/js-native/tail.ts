/**
 * Finite raw-track interop: publish until the harness acknowledges, or read through clean EOF.
 *
 * `drop` cuts group 1. `join` reads from whichever group the subscription starts at: a
 * moq-transport subscription starts at the largest group rather than at the range asked for, so
 * it must read a contiguous run of groups through the last one. `compress` publishes a compressed
 * track with several frames, one empty, in each group. `cached` publishes every group before
 * announcing, for a FETCH of a past group. `late` writes the head of group 0 before announcing and
 * the rest only once the harness relays that the subscriber read it, so a join that skips the head
 * stalls rather than racing the publisher.
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
// The groups a `late` track publishes, and how many frames of group 0 precede the subscriber.
const LATE_GROUPS = 2;
const LATE_HEAD = 2;
const [role, url, path, version, mode] = process.argv.slice(2);
const drop = mode === "drop";
const compress = mode === "compress";
console.debug = console.error;
console.info = console.error;
const join = mode === "join" || mode === "status";
const datagram = mode === "datagram";
const status = mode === "status";
const cached = mode === "cached";
const late = mode === "late";
if (
	!url ||
	!path ||
	!["publish", "subscribe"].includes(role) ||
	![undefined, "drop", "join", "compress", "datagram", "status", "cached", "late"].includes(mode)
) {
	throw new Error(
		"usage: tail.ts publish|subscribe URL BROADCAST [VERSION [drop|join|compress|datagram|status|cached|late]]",
	);
}

/**
 * The frames of each group. A compressed track spans several frames, one empty, per window.
 * `cached` and `late` give every frame its own bytes, so a FETCH or join that drops, repeats, or
 * reorders one fails.
 */
function frames(sequence: number): Uint8Array[] {
	if (compress)
		return [new Uint8Array(BYTES / 2).fill(sequence), new Uint8Array(0), new Uint8Array(BYTES / 2).fill(sequence)];
	if (cached || late) return [0, 1, 2].map((index) => new Uint8Array(BYTES / 4).fill(sequence * 16 + index));
	return [new Uint8Array(BYTES).fill(sequence)];
}

/** Publish every group, cutting group 1 for `drop`. The end precedes the last group's data. */
function write(track: Moq.Track.Producer) {
	track.finishAt({ group: GROUPS, frame: 0 });
	for (let sequence = 0; sequence < GROUPS; sequence++) {
		const group = track.appendGroup();
		if (drop && sequence === 1) {
			group.abort(new Error("cut"));
			continue;
		}
		for (const payload of frames(sequence)) group.writeFrame({ payload });
		group.finish();
	}
	track.finish();
}

/** Read one whole group, checking every frame's bytes, and report when the `late` head is in. */
async function read(group: Moq.Group.Consumer) {
	for (const [index, want] of frames(group.sequence).entries()) {
		const frame = await group.readFrame();
		assert(frame, "short group");
		assert(Buffer.from(frame.payload).equals(want), `wrong frame ${index} in group ${group.sequence}`);
		if (late && group.sequence === 0 && index + 1 === LATE_HEAD) process.stdout.write("late head\n");
	}
	assert.equal(await group.readFrame(), undefined, "extra frame");
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
		if (cached) write(track);
		let head: Moq.Group.Producer | undefined;
		if (late) {
			head = track.appendGroup();
			for (const payload of frames(0).slice(0, LATE_HEAD)) head.writeFrame({ payload });
		}
		broadcastPublication.announce();
		const lines = createInterface({ input: process.stdin });
		const commands = lines[Symbol.asyncIterator]();
		if (head) {
			assert.equal((await commands.next()).value, "late tail", "subscriber did not read the head");
			for (const payload of frames(0).slice(LATE_HEAD)) head.writeFrame({ payload });
			head.finish();
			for (let sequence = 1; sequence < LATE_GROUPS; sequence++) {
				const group = track.appendGroup();
				for (const payload of frames(sequence)) group.writeFrame({ payload });
				group.finish();
			}
			track.finish();
			track.close();
		} else if (!status && !cached) {
			const demand = track.demand();
			while (demand.wanted.peek() === undefined) await demand.wanted.changed();
			if (datagram) {
				for (let sequence = 0; sequence < GROUPS; sequence++) {
					assert.equal((await commands.next()).value, `datagram ${sequence}`, "missing datagram command");
					track.appendDatagram(undefined, new Uint8Array(900).fill(sequence));
				}
			} else {
				write(track);
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
		const request = origin.follow(Moq.Path.from(path));
		try {
			let broadcast = request.active.peek();
			while (!broadcast) {
				await request.active.changed();
				broadcast = request.active.peek();
			}
			// `late` names no start, joining the latest group as a player does: group 0, whose
			// head predates the subscription.
			const track = broadcast.track("tail").subscribe({
				maxDelay: Milli(drop ? 60_000 : 5000),
				...(late ? {} : { groups: { start: { included: 0 } } }),
			});
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
				} else if (late) {
					for (let sequence = 0; sequence < LATE_GROUPS; sequence++) {
						const group = await track.recvGroup();
						assert(group, "missing group");
						try {
							assert.equal(group.sequence, sequence, "joined the wrong group");
							await read(group);
						} finally {
							group.close();
						}
					}
					assert.equal(await track.recvGroup(), undefined, "extra group");
					assert.deepEqual(await track.finished(), { group: LATE_GROUPS, frame: 0 });
					console.error("late join clean groups=0,1 frames=6");
				} else {
					for (;;) {
						const group = await track.recvGroup();
						if (!group) break;
						try {
							assert(group.sequence < GROUPS, `unexpected group ${group.sequence}`);
							await read(group);
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

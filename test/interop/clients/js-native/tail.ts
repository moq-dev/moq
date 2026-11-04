/**
 * Finite raw-track interop: publish until the harness acknowledges, or read through clean EOF.
 *
 * `drop` cuts group 1. `join` reads from whichever group the subscription starts at: a
 * moq-transport subscription starts at the largest group rather than at the range asked for, so
 * it must read a contiguous run of groups through the last one. `compress` publishes a compressed
 * track with several frames, one empty, in each group. `cached` publishes every group before
 * announcing, for a FETCH of a past group. `history` writes only the last group live and serves the
 * ones below it, never cached, from an in-memory responder on `track.requests()`. `late` writes the head of group 0 before announcing and
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
const status = mode === "status";
const cached = mode === "cached";
const history = mode === "history";
const late = mode === "late";
if (
	!url ||
	!path ||
	!["publish", "subscribe"].includes(role) ||
	![undefined, "drop", "join", "compress", "status", "cached", "history", "late"].includes(mode)
) {
	throw new Error(
		"usage: tail.ts publish|subscribe URL BROADCAST [VERSION [drop|join|compress|status|cached|history|late]]",
	);
}

/**
 * The frames of each group. A compressed track spans several frames, one empty, per window.
 * `cached`, `history`, and `late` give every frame its own bytes, so a FETCH or join that drops, repeats, or
 * reorders one fails.
 */
function frames(sequence: number): Uint8Array[] {
	if (compress)
		return [new Uint8Array(BYTES / 2).fill(sequence), new Uint8Array(0), new Uint8Array(BYTES / 2).fill(sequence)];
	if (cached || history || late)
		return [0, 1, 2].map((index) => new Uint8Array(BYTES / 4).fill(sequence * 16 + index));
	return [new Uint8Array(BYTES).fill(sequence)];
}

/** Publish every group, cutting group 1 for `drop`. The end precedes the last group's data. */
function write(track: Moq.Track.Producer) {
	track.finishAt({ group: BigInt(GROUPS), frame: 0 });
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

/**
 * Write only the last group, and serve the groups below it from an in-memory responder: this
 * producer never cached them, as one resumed past its storage would not have.
 */
function serveHistory(track: Moq.Track.Producer) {
	track.finishAt({ group: BigInt(GROUPS), frame: 0 });
	const live = track.createGroup(BigInt(GROUPS - 1));
	for (const payload of frames(GROUPS - 1)) live.writeFrame({ payload });
	live.finish();
	const requests = track.requests();
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			for (let sequence = fill.wanted(); sequence !== undefined; sequence = fill.wanted()) {
				const group = fill.accept(sequence);
				for (const payload of frames(Number(sequence))) group.writeFrame({ payload });
				group.finish();
				console.error(`history served ${sequence}`);
			}
			fill.close();
		}
	})();
}

/** Read one whole group, checking every frame's bytes, and report when the `late` head is in. */
async function read(group: Moq.Group.Consumer) {
	for (const [index, want] of frames(Number(group.sequence)).entries()) {
		const frame = await group.readFrame();
		assert(frame, "short group");
		assert(Buffer.from(frame.payload).equals(want), `wrong frame ${index} in group ${group.sequence}`);
		if (late && group.sequence === 0n && index + 1 === LATE_HEAD) process.stdout.write("late head\n");
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
		// `cached` and `late` read groups a newer one supersedes, which the default zero max age
		// would leave stale.
		const track = broadcast.createTrack("tail", { compress, maxAge: null, ...(status ? { priority: 2 } : {}) });
		if (cached) write(track);
		if (history) serveHistory(track);
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
		} else if (!status && !cached && !history) {
			const demand = track.demand();
			while (demand.wanted.peek() === undefined) await demand.wanted.changed();
			write(track);
			track.close();
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
				delay: { max: Milli(drop ? 60_000 : 5000) },
				...(late ? {} : { ranges: [{ start: { absolute: 0n } }] }),
			});
			if (compress) assert.equal((await track.info()).compress, version === "moq-lite-07-wip");
			const seen: number[] = [];
			try {
				if (late) {
					for (let sequence = 0; sequence < LATE_GROUPS; sequence++) {
						const group = await track.recvGroup();
						assert(group, "missing group");
						try {
							assert.equal(group.sequence, BigInt(sequence), "joined the wrong group");
							await read(group);
						} finally {
							group.close();
						}
					}
					assert.equal(await track.recvGroup(), undefined, "extra group");
					assert.deepEqual(await track.finished(), { group: BigInt(LATE_GROUPS), frame: 0 });
					console.error("late join clean groups=0,1 frames=6");
				} else {
					for (;;) {
						const group = await track.recvGroup();
						if (!group) break;
						try {
							assert(group.sequence < GROUPS, `unexpected group ${group.sequence}`);
							await read(group);
							seen.push(Number(group.sequence));
						} finally {
							group.close();
						}
					}
					seen.sort((a, b) => a - b);
					const first = join ? (seen[0] ?? 0) : 0;
					const all = Array.from({ length: GROUPS - first }, (_, i) => first + i);
					assert.deepEqual(seen, drop ? [0, 2, 3] : all);
					assert.deepEqual(await track.finished(), { group: BigInt(GROUPS), frame: 0 });
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

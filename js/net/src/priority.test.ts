import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { Version } from "./lite/version.ts";
import { Priority, sendOrder } from "./priority.ts";
import { type SendStream, Writer } from "./stream.ts";
import { Timescale } from "./time.ts";
import type { Subscription } from "./track.ts";

// The last position that still fits below the track priorities.
const MAX_POSITION = 2 ** 36 - 1;

// The transport sends higher values first, so a track the subscriber cares about more has to
// outrank one it cares about less, wherever their groups sit in their own queues.
test("track priority dominates the position", () => {
	expect(sendOrder({ priority: 2, position: 1_000_000 })).toBeGreaterThan(sendOrder({ priority: 1, position: 0 }));
	expect(sendOrder({ priority: 127, position: MAX_POSITION })).toBeGreaterThan(
		sendOrder({ priority: 126, position: 0 }),
	);
	expect(sendOrder({ priority: 0, position: 0 })).toBeGreaterThan(sendOrder({ priority: -1, position: 0 }));
});

// The publisher's priority breaks a tie at one subscriber priority, as in the Rust
// `Priority::cmp`, but never outranks a higher subscriber priority.
test("the publisher priority breaks subscriber ties", () => {
	expect(sendOrder({ priority: 0, publisher: 80 })).toBeGreaterThan(sendOrder({ priority: 0, publisher: 60 }));
	expect(sendOrder({ priority: 0, publisher: 1, position: MAX_POSITION })).toBeGreaterThan(
		sendOrder({ priority: 0, publisher: 0, position: 0 }),
	);
	expect(sendOrder({ priority: 1, publisher: -128 })).toBeGreaterThan(sendOrder({ priority: 0, publisher: 127 }));
	expect(sendOrder({ priority: 0 })).toBe(sendOrder({ priority: 0, publisher: 0 }));
});

// Position 0 is the group its subscription wants sent next, so it goes first.
test("an earlier position outranks a later one", () => {
	expect(sendOrder({ priority: 1, position: 0 })).toBeGreaterThan(sendOrder({ priority: 1, position: 1 }));
	expect(sendOrder({ priority: 1, position: 1 })).toBeGreaterThan(sendOrder({ priority: 1, position: 2 }));
});

// The reason positions are used instead of sequences: two tracks at the same priority each get
// their next group out, rather than the one with larger group numbers starving the other.
test("equal priorities tie at the same position", () => {
	expect(sendOrder({ priority: 3, position: 0 })).toBe(sendOrder({ priority: 3, position: 0 }));
	expect(sendOrder({ priority: 3 })).toBe(sendOrder({ priority: 3, position: 0 }));
});

// Every send order stays a safe integer, so two distinct ranks never collapse into one.
test("send orders stay safe integers", () => {
	expect(sendOrder({ priority: 127, publisher: 127, position: 0 })).toBe(-1);
	expect(sendOrder({ priority: -128, publisher: -128, position: MAX_POSITION })).toBe(-(2 ** 52));
	expect(Number.isSafeInteger(sendOrder({ priority: -128, publisher: -128, position: MAX_POSITION }))).toBe(true);
});

// Protocol streams use the transport's default order of 0. Keeping every group below it
// prevents a continuous supply of media from starving subscriptions, announcements, or probes.
test("protocol streams outrank group data", () => {
	expect(sendOrder({ priority: 127, publisher: 127, position: 0 })).toBeLessThan(0);
});

// A position past the space left for it must never carry into the track's bits and outrank a
// higher-priority track.
test("out of range ranks stay inside their own bits", () => {
	expect(sendOrder({ priority: 1, position: Number.MAX_SAFE_INTEGER })).toBeLessThan(
		sendOrder({ priority: 2, position: 0 }),
	);
	expect(sendOrder({ priority: 300, position: 0 })).toBe(sendOrder({ priority: 127, position: 0 }));
	expect(sendOrder({ priority: -300, position: -1 })).toBe(sendOrder({ priority: -128, position: 0 }));
});

// The ranking itself, driven directly: a subscription outlives its own serving loop, since a
// group already on the wire keeps writing after the track stops handing out new ones.
function ranking(options: Subscription = {}) {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const subscriber = track.subscribe(options);

	// The send order lands on the underlying stream, exactly as it would on a real one.
	const streams: SendStream[] = [];
	const open = () => {
		const stream = new WritableStream<Uint8Array>() as SendStream;
		streams.push(stream);
		return new Writer(stream, Version.DRAFT_06);
	};

	return { priority: new Priority(subscriber), subscriber, open, streams, broadcast };
}

// A track that finishes while several groups are still draining leaves them as the only data
// the subscriber is still waiting on. Stranding them at the position they held when the track
// ended would let an equal-priority live subscription preempt them until they complete.
test("groups still draining are promoted after the subscription closes", () => {
	const { priority, open, streams, broadcast } = ranking({ priority: 7 });

	const older = open();
	const newer = open();
	priority.add(older, 0, 0);
	priority.add(newer, 1, 0);
	expect(streams[0].sendOrder).toBe(sendOrder({ priority: 7, position: 1 }));

	// The serving loop is done, but both groups are still on the wire.
	priority.close();
	expect(streams[0].sendOrder).toBe(sendOrder({ priority: 7, position: 1 }));

	// The newer group finishes, so the older one is now the one to send next.
	priority.remove(newer);
	expect(streams[0].sendOrder).toBe(sendOrder({ priority: 7, position: 0 }));

	priority.remove(older);
	broadcast.close();
});

// An ascending subscription drains its backlog oldest first, and changing the order re-ranks
// the groups already on the wire rather than only the ones opened after.
test("the subscription's order picks which group goes first", async () => {
	const { priority, subscriber, open, streams, broadcast } = ranking({ priority: 7, order: "ascending" });

	const writers = [5, 1, 10].map((sequence) => {
		const writer = open();
		priority.add(writer, sequence, 0);
		return writer;
	});
	const at = (...positions: number[]) => positions.map((position) => sendOrder({ priority: 7, position }));

	// Groups 5, 1, 10: oldest first.
	expect(streams.map((stream) => stream.sendOrder)).toEqual(at(1, 0, 2));

	const changed = subscriber.subscription.changed();
	subscriber.update({ priority: 7, order: "descending" });
	await changed;
	expect(streams.map((stream) => stream.sendOrder)).toEqual(at(1, 2, 0));

	priority.close();
	for (const writer of writers) priority.remove(writer);
	broadcast.close();
});

// A joining fill starts earlier in the same group, even when the live rest opened first.
test.each(["ascending", "descending"] as const)("a fill outranks its group's live rest in %s order", (order) => {
	const { priority, open, streams, broadcast } = ranking({ priority: 7, order });
	const rest = open();
	const fill = open();
	const other = open();
	priority.add(rest, 5, 2 ** 45);
	priority.add(fill, 5, 0);
	priority.add(other, order === "ascending" ? 4 : 6, Number.MAX_SAFE_INTEGER);
	const [restOrder, fillOrder, otherOrder] = streams.map((stream) => {
		if (stream.sendOrder === undefined) throw new Error("send order missing");
		return stream.sendOrder;
	});
	expect(fillOrder).toBeGreaterThan(restOrder);
	expect(otherOrder).toBeGreaterThan(fillOrder);
	priority.remove(fill);
	expect(streams[0].sendOrder).toBe(sendOrder({ priority: 7, position: 1 }));
	priority.close();
	priority.remove(rest);
	priority.remove(other);
	broadcast.close();
});

import { expect, test } from "bun:test";
import { Group } from "./group.ts";
import { TrackProducer } from "./track.ts";

test("nextGroup skips late arrivals", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	producer.writeGroup(new Group(5));

	const first = await track.nextGroup();
	expect(first?.sequence).toBe(5);

	// Late arrivals with sequence <= last returned are skipped.
	producer.writeGroup(new Group(3));
	producer.writeGroup(new Group(4));
	producer.writeGroup(new Group(7));

	const next = await track.nextGroup();
	expect(next?.sequence).toBe(7);
});

test("nextGroup returns buffered groups in sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	producer.writeGroup(new Group(3));
	producer.writeGroup(new Group(5));

	expect((await track.nextGroup())?.sequence).toBe(3);
	expect((await track.nextGroup())?.sequence).toBe(5);
});

test("recvGroup after nextGroup still returns late arrivals", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	producer.writeGroup(new Group(5));

	// Ordered returns seq 5, advancing its cursor.
	const ordered = await track.nextGroup();
	expect(ordered?.sequence).toBe(5);

	// recvGroup is independent of the ordered cursor: a late seq 3 still surfaces.
	producer.writeGroup(new Group(3));
	const recv = await track.recvGroup();
	expect(recv?.sequence).toBe(3);
});

test("nextGroup returns undefined when track closes", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	producer.close();
	expect(await track.nextGroup()).toBeUndefined();
});

test("readFrame does not livelock when a sole group finishes before the next arrives", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	// A group is appended then finished empty while the track stays open. A finished group's
	// readable() resolves immediately, so the reader must not busy-wait on it (which would starve the
	// macrotask queue and never observe the next group).
	const g0 = producer.appendGroup();
	g0.close();

	// The next group arrives via a macrotask; if the reader livelocks on microtasks it never runs.
	setTimeout(() => {
		const g1 = producer.appendGroup();
		g1.writeString("hello");
		g1.close();
		producer.close();
	}, 10);

	expect(await track.readString()).toBe("hello");
}, 2000);

import { expect, test } from "bun:test";
import { Group } from "./group.ts";
import { Track } from "./track.ts";

test("nextGroupOrdered skips late arrivals", async () => {
	const track = new Track("test");

	track.writeGroup(new Group(5));

	const first = await track.nextGroupOrdered();
	expect(first?.sequence).toBe(5);

	// Late arrivals with sequence <= last returned are skipped.
	track.writeGroup(new Group(3));
	track.writeGroup(new Group(4));
	track.writeGroup(new Group(7));

	const next = await track.nextGroupOrdered();
	expect(next?.sequence).toBe(7);
});

test("nextGroupOrdered returns buffered groups in sequence", async () => {
	const track = new Track("test");

	track.writeGroup(new Group(3));
	track.writeGroup(new Group(5));

	expect((await track.nextGroupOrdered())?.sequence).toBe(3);
	expect((await track.nextGroupOrdered())?.sequence).toBe(5);
});

test("recvGroup after nextGroupOrdered still returns late arrivals", async () => {
	const track = new Track("test");

	track.writeGroup(new Group(5));

	// Ordered returns seq 5, advancing its cursor.
	const ordered = await track.nextGroupOrdered();
	expect(ordered?.sequence).toBe(5);

	// recvGroup is independent of the ordered cursor: a late seq 3 still surfaces.
	track.writeGroup(new Group(3));
	const recv = await track.recvGroup();
	expect(recv?.sequence).toBe(3);
});

test("nextGroupOrdered returns undefined when track closes", async () => {
	const track = new Track("test");
	track.close();
	expect(await track.nextGroupOrdered()).toBeUndefined();
});
